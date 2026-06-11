import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_33174277 extends AI {
public AI_33174277() throws LeekRunException {
super(3, 2);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
Object u_a = ops(1l, 1);
if (ops(ops(eq(u_a, 1l), 2) && true, 1)) {
return 7l;
}
return 0l;
}
protected String getAIString() { return "Main_1a99d42cb17e2_36";}
protected String[] getErrorFiles() { return new String[] {"Main_1a99d42cb17e2_36", };}

protected int[] getErrorFilesID() { return new int[] {33174277, };}

}
