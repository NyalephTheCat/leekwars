import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_1 extends AI {
public AI_1() throws LeekRunException {
super(2, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_square(Object p_n) throws LeekRunException {var u_n = p_n;
ops(1);ops(2); return (Object) mul(u_n, u_n);
}
public Object runIA(Session session) throws LeekRunException {
Object u_x = ops(f_square(7l), 1);
return u_x;
}
protected String getAIString() { return "03_function_call.leek";}
protected String[] getErrorFiles() { return new String[] {"03_function_call.leek", };}

protected int[] getErrorFilesID() { return new int[] {1, };}

}
