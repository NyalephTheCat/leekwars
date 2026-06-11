import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_42856534 extends AI {
public AI_42856534() throws LeekRunException {
super(1, 2);
}
public void staticInit() throws LeekRunException {
}
private Object f_f() throws LeekRunException {
ops(1);Object u_a = ops(1l, 1);
Object u_b = ops(2l, 1);
if (ops(ops(eq(u_a, 1l), 2) && ops(eq(u_b, 2l), 1), 1)) {
return 42l;
}
return 0l;
}
public Object runIA(Session session) throws LeekRunException {
return f_f();
}
protected String getAIString() { return "Main_1a99cf44f3b87_9";}
protected String[] getErrorFiles() { return new String[] {"sub", "Main_1a99cf44f3b87_9", };}

protected int[] getErrorFilesID() { return new int[] {115201, 42856534, };}

}
