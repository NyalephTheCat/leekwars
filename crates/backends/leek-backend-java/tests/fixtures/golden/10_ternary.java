import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_1 extends AI {
public AI_1() throws LeekRunException {
super(3, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
Object u_n = ops(7l, 1);
Object u_result = ops(more(u_n, 0l) ? u_n : ops(minus(u_n), 1), 3);
return u_result;
}
protected String getAIString() { return "10_ternary.leek";}
protected String[] getErrorFiles() { return new String[] {"10_ternary.leek", };}

protected int[] getErrorFilesID() { return new int[] {1, };}

}
